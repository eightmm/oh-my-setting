#!/usr/bin/env python3
"""Small content-free W3C Trace Context validator used by OMS writers."""

from __future__ import annotations

import os
import re
import secrets
from typing import Any, Dict, Mapping, Optional


TRACEPARENT_RE = re.compile(
    r"^([0-9a-f]{2})-([0-9a-f]{32})-([0-9a-f]{16})-([0-9a-f]{2})$"
)
TRACE_ID_RE = re.compile(r"^[0-9a-f]{32}$")
SPAN_ID_RE = re.compile(r"^[0-9a-f]{16}$")
FLAGS_RE = re.compile(r"^[0-9a-f]{2}$")


def parse_traceparent(value: Any) -> Optional[Dict[str, str]]:
    if not isinstance(value, str) or len(value) > 128:
        return None
    match = TRACEPARENT_RE.fullmatch(value)
    if match is None:
        return None
    version, trace_id, parent_span_id, flags = match.groups()
    if version == "ff" or trace_id == "0" * 32 or parent_span_id == "0" * 16:
        return None
    return {
        "trace_id": trace_id,
        "parent_span_id": parent_span_id,
        "trace_flags": flags,
    }


def persisted_context(row: Mapping[str, Any]) -> Optional[Dict[str, str]]:
    trace_id = row.get("trace_id")
    span_id = row.get("span_id")
    flags = row.get("trace_flags", "00")
    if (
        not isinstance(trace_id, str)
        or not TRACE_ID_RE.fullmatch(trace_id)
        or trace_id == "0" * 32
        or not isinstance(span_id, str)
        or not SPAN_ID_RE.fullmatch(span_id)
        or span_id == "0" * 16
        or not isinstance(flags, str)
        or not FLAGS_RE.fullmatch(flags)
    ):
        return None
    parent = row.get("parent_span_id")
    if parent is not None and (
        not isinstance(parent, str)
        or not SPAN_ID_RE.fullmatch(parent)
        or parent == "0" * 16
    ):
        return None
    result = {"trace_id": trace_id, "span_id": span_id, "trace_flags": flags}
    if isinstance(parent, str):
        result["parent_span_id"] = parent
    return result


def attach_trace_context(
    row: Dict[str, Any], previous: Optional[Mapping[str, Any]] = None
) -> Dict[str, Any]:
    """Attach IDs only; never persist traceparent, tracestate, or baggage."""

    inherited = persisted_context(previous or {})
    if inherited is not None:
        trace_id = inherited["trace_id"]
        parent_span_id = inherited["span_id"]
        flags = inherited["trace_flags"]
    else:
        incoming = parse_traceparent(os.environ.get("OMS_TRACEPARENT", ""))
        if incoming is None:
            return row
        trace_id = incoming["trace_id"]
        parent_span_id = incoming["parent_span_id"]
        flags = incoming["trace_flags"]
    row["trace_id"] = trace_id
    row["span_id"] = secrets.token_hex(8)
    row["parent_span_id"] = parent_span_id
    row["trace_flags"] = flags
    return row


def child_traceparent(row: Mapping[str, Any]) -> str:
    context = persisted_context(row)
    if context is None:
        return ""
    return "00-%s-%s-%s" % (
        context["trace_id"],
        context["span_id"],
        context["trace_flags"],
    )
