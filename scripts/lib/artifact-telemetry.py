#!/usr/bin/env python3
"""Read-only telemetry over the retained cross-agent artifact index."""

from __future__ import annotations

import argparse
import datetime
import json
import os
import re
import statistics
import sys
from collections import Counter
from typing import Optional


ELIGIBLE_KINDS = frozenset(("call", "ask", "review", "delegate"))
# Current markers plus the retired tier vocabulary: telemetry reads history,
# and rows recorded before de-tiering must stay legible, not "unrecorded".
MODEL_CLASSES = frozenset(
    ("explicit", "provider-default", "role-default", "fast", "balanced", "deep")
)
STARTED_RE = re.compile(r"(?m)^- started:\s*(\S+)\s*$")
TOKENS_RE = re.compile(
    r"(?im)^tokens used[ \t]*\r?\n[ \t]*([0-9][0-9,]*)[ \t]*$"
)
# The transport parsers leave these beside "tokens used": the model the
# provider says actually ran (a provider-default route records none itself)
# and the cost where the provider reports one.
SERVED_MODEL_RE = re.compile(
    r"(?im)^served model[ \t]*\r?\n[ \t]*([A-Za-z0-9][A-Za-z0-9._:+/-]{0,199})[ \t]*$"
)
CONFIGURED_MODEL_RE = re.compile(
    r"(?im)^configured model[ \t]*\r?\n[ \t]*([A-Za-z0-9][A-Za-z0-9._:+/-]{0,199})[ \t]*$"
)
COST_RE = re.compile(r"(?im)^cost usd[ \t]*\r?\n[ \t]*([0-9]+(?:\.[0-9]+)?)[ \t]*$")
ARTIFACT_EDGE_BYTES = 64 * 1024


def integer(value: object) -> Optional[int]:
    if isinstance(value, bool) or not isinstance(value, int) or value < 0:
        return None
    return value


def parse_timestamp(value: object) -> Optional[datetime.datetime]:
    if not isinstance(value, str) or not value:
        return None
    normalized = value[:-1] + "+00:00" if value.endswith("Z") else value
    try:
        parsed = datetime.datetime.fromisoformat(normalized)
    except ValueError:
        return None
    if parsed.tzinfo is None:
        parsed = parsed.replace(tzinfo=datetime.timezone.utc)
    return parsed


def inside(path: str, root: str) -> bool:
    try:
        return os.path.commonpath((path, root)) == root
    except ValueError:
        return False


def cached_duration(row: dict[str, object]) -> Optional[float]:
    value = row.get("duration_s")
    if isinstance(value, bool) or not isinstance(value, (int, float)) or value < 0:
        return None
    return round(float(value), 3)


def artifact_text(repo: str, row: dict[str, object]) -> Optional[str]:
    """The artifact's head and tail — the footers live at the end — or None
    when the row names no readable artifact inside the repository."""
    artifact = row.get("artifact")
    if not isinstance(artifact, str) or not artifact or os.path.isabs(artifact):
        return None
    candidate = os.path.realpath(os.path.join(repo, artifact))
    if not inside(candidate, repo) or not os.path.isfile(candidate):
        return None
    try:
        with open(candidate, "rb") as handle:
            size = os.fstat(handle.fileno()).st_size
            if size <= ARTIFACT_EDGE_BYTES * 2:
                raw = handle.read()
            else:
                head = handle.read(ARTIFACT_EDGE_BYTES)
                handle.seek(-ARTIFACT_EDGE_BYTES, os.SEEK_END)
                raw = head + b"\n" + handle.read(ARTIFACT_EDGE_BYTES)
    except OSError:
        return None
    return raw.decode("utf-8", errors="replace")


def metric_output(text: str, row: dict[str, object]) -> str:
    """Read provider sections, not quoted prompts, verifier logs or summaries."""
    lines = []
    active = repair = False
    synthesis = "## Synthesis (%s)" % row.get("provider")
    for line in text.splitlines():
        if line == "## Output" or (
            row.get("kind") == "review-synthesis" and row.get("provider") != "local" and line == synthesis
        ):
            lines = []
            active = True
            repair = False
        elif re.fullmatch(r"## Repair [0-9]+", line):
            active, repair = False, True
        elif repair and line == "### Output":
            active = True
        elif line in ("## Prompt", "### Prompt", "## Exit") or re.fullmatch(r"## Verify(?: \(repair [0-9]+\))?", line):
            active = False
        elif active:
            lines.append(line)
    return "\n".join(lines)


def artifact_usage_metrics(repo: str, row: dict[str, object]) -> Optional[dict]:
    saved = row.get("provider_usage")
    if isinstance(saved, dict):
        return saved
    text = artifact_text(repo, row)
    if text is None:
        return None
    text = metric_output(text, row)
    reports = []
    for raw in re.findall(r"(?m)^usage detail: (\{[^\r\n]+\})$", text):
        try:
            report = json.loads(raw)
        except ValueError:
            continue
        if isinstance(report, dict) and report.get("provider") == row.get("provider"):
            reports.append(report)
    if not reports:
        return None
    result = {"reports": len(reports), "measurement": "provider-reported"}
    cumulative = [report for report in reports if report.get("session_cumulative") is True]
    measured = cumulative[-1:] if cumulative else reports
    if cumulative:
        result["session_cumulative"] = True
    for key in ("input_tokens", "output_tokens", "cache_read_tokens", "cache_write_tokens", "tool_calls"):
        values = [integer(report.get(key)) for report in measured]
        result[key] = sum(values) if all(value is not None for value in values) else None
    session_totals = measured[-1].get("session_totals") if measured else None
    if isinstance(session_totals, dict):
        keys = ("input_tokens", "output_tokens", "cached_input_tokens")
        values = [integer(session_totals.get(key)) for key in keys]
        if all(value is not None for value in values):
            result["session_totals"] = dict(zip(keys, values))
    flags = [report.get("cache_in_input") for report in measured]
    result["cache_in_input"] = flags[0] if type(flags[0]) is bool and all(flag is flags[0] for flag in flags) else None
    return result


def artifact_model_metrics(
    repo: str, row: dict[str, object]
) -> dict[str, object]:
    """Return bounded model provenance and operation-level provider cost.

    Cost may span retries, but a per-model identity is published only when the
    whole operation has one attributable route. Multi-model or fallback rows
    keep their total cost while refusing a misleading model assignment.
    """
    saved_model = row.get("served_model")
    saved_model = saved_model if isinstance(saved_model, str) and saved_model else None
    saved_configured = row.get("configured_model")
    saved_configured = saved_configured if isinstance(saved_configured, str) and saved_configured else None
    saved_cost = row.get("cost_usd")
    saved_cost = float(saved_cost) if isinstance(saved_cost, (int, float)) and not isinstance(saved_cost, bool) else None
    text = artifact_text(repo, row)
    if text is not None:
        text = metric_output(text, row)
        models = SERVED_MODEL_RE.findall(text)
        configured = CONFIGURED_MODEL_RE.findall(text)
        costs = COST_RE.findall(text)
    else:
        models, configured, costs = [], [], []
    if saved_model:
        models.append(saved_model)
    if saved_configured:
        configured.append(saved_configured)

    # The first implementation joined multiple Claude modelUsage keys with
    # '+'. Split that historical shape so old internally-fallback calls become
    # ambiguous instead of a synthetic model name.
    expanded = []
    for model in models:
        expanded.extend(part for part in model.split("+") if part)
    models = list(dict.fromkeys(expanded))
    configured = list(dict.fromkeys(configured))
    cost = saved_cost if saved_cost is not None else (round(sum(float(c) for c in costs), 6) if costs else None)
    ambiguous = bool(row.get("fallback_used")) or len(models) > 1 or len(configured) > 1
    if models and configured and set(models) != set(configured):
        ambiguous = True
    if row.get("model_attribution") == "ambiguous":
        ambiguous = True
    if ambiguous:
        return {
            "served_model": None,
            "configured_model": None,
            "cost_usd": cost,
            "model_attribution": "ambiguous",
        }
    if models:
        return {
            "served_model": models[0],
            "configured_model": None,
            "cost_usd": cost,
            "model_attribution": "transport",
        }
    if configured:
        return {
            "served_model": None,
            "configured_model": configured[0],
            "cost_usd": cost,
            "model_attribution": "configured-default",
        }
    return {
        "served_model": None,
        "configured_model": None,
        "cost_usd": cost,
        "model_attribution": "unknown",
    }


def artifact_metrics(
    repo: str, row: dict[str, object]
) -> tuple[bool, Optional[float], Optional[int]]:
    """Metrics for one row: cached numbers first, the artifact only as fallback.

    Duration and tokens used to be readable exclusively by parsing the artifact,
    so retention decided accounting — deleting stale artifacts silently erased
    the record of what those calls cost. Rows written since carry the numbers,
    and the parse remains for everything written before.
    """
    saved_duration = cached_duration(row)
    saved_tokens = integer(row.get("tokens"))

    text = artifact_text(repo, row)
    if text is None:
        return False, saved_duration, saved_tokens

    token_matches = TOKENS_RE.findall(metric_output(text, row))
    tokens = sum(int(value.replace(",", "")) for value in token_matches) if token_matches else None
    started_match = STARTED_RE.search(text)
    started = parse_timestamp(started_match.group(1)) if started_match else None
    finished = parse_timestamp(row.get("ts"))
    duration = None
    if started is not None and finished is not None:
        seconds = (finished - started).total_seconds()
        if seconds >= 0:
            duration = round(seconds, 3)
    if saved_duration is not None:
        duration = saved_duration
    if saved_tokens is not None:
        tokens = saved_tokens
    return True, duration, tokens


def resolved_events(rows: list[dict[str, object]]) -> set[str]:
    counts = Counter(
        row.get("event_id")
        for row in rows
        if isinstance(row.get("event_id"), str)
    )
    by_id = {
        row.get("event_id"): (position, row)
        for position, row in enumerate(rows)
        if isinstance(row.get("event_id"), str)
        and counts[row.get("event_id")] == 1
    }
    resolved: set[str] = set()
    for position, resolver in enumerate(rows):
        if resolver.get("kind") != "artifact-resolution":
            continue
        target_id = resolver.get("resolves_event_id")
        resolver_id = resolver.get("event_id")
        if not isinstance(target_id, str) or not isinstance(resolver_id, str):
            continue
        target_entry = by_id.get(target_id)
        if (
            not target_entry
            or resolver.get("schema") != 1
            or counts[resolver_id] != 1
            or resolver.get("parent_event_id") != target_id
            or resolver.get("resolution") != "resolved"
        ):
            continue
        target_position, target = target_entry
        resolver_exit = integer(resolver.get("exit"))
        target_exit = integer(target.get("exit"))
        if (
            resolver_exit == 0
            and target_exit is not None
            and target_exit > 0
            and target_position < position
            and target.get("schema") == 1
            and target.get("kind") != "artifact-resolution"
            and resolver.get("operation_id") == target.get("operation_id")
            and resolver.get("artifact_id") == target.get("artifact_id")
        ):
            resolved.add(str(target_id))
    return resolved


def empty_route(provider: str, model_class: str, selected_model: str) -> dict:
    return {
        "provider": provider,
        "model_class": model_class,
        "selected_model": selected_model,
        "operations": 0,
        "recorded_exit_zero": 0,
        "recorded_exit_nonzero": 0,
        "recorded_exit_unavailable": 0,
        "verification_recorded": 0,
        "verification_nonzero": 0,
        "fallback_used": 0,
        "token_reports": 0,
        "provider_reported_tokens": 0,
        "usage_detail": {"reports": 0, "input_tokens": 0, "cache_read_tokens": 0, "output_tokens": 0},
        "duration_reports": 0,
        "wall_seconds": 0.0,
        "verified_success": 0,
        "verified_failure": 0,
        "outcome_unknown": 0,
    }


def native_activity(repo: str) -> dict:
    path = os.path.join(repo, ".oms", "hooks", "events.jsonl")
    rows: list[dict[str, object]] = []
    invalid = 0
    if os.path.isfile(path):
        with open(path, encoding="utf-8", errors="replace") as handle:
            for line in handle:
                try:
                    row = json.loads(line)
                except (TypeError, ValueError):
                    invalid += 1
                    continue
                if isinstance(row, dict) and row.get("action") == "telemetry":
                    rows.append(row)

    sessions = {
        row.get("session") for row in rows
        if isinstance(row.get("session"), str) and row.get("session")
    }
    turns = {
        (row.get("session"), row.get("turn_id")) for row in rows
        if isinstance(row.get("session"), str)
        and isinstance(row.get("turn_id"), str)
        and row.get("turn_id")
    }
    usage_keys = (
        "input_tokens", "output_tokens", "cache_read_tokens",
        "cache_creation_tokens", "reasoning_tokens",
    )
    # Providers differ on whether a hook reports a delta or a cumulative
    # session value. Per-session maxima avoid double-counting cumulative rows.
    by_session: dict[str, dict[str, int]] = {}
    for row in rows:
        session = row.get("session")
        if not isinstance(session, str) or not session:
            continue
        totals = by_session.setdefault(session, {})
        for key in usage_keys:
            value = integer(row.get(key))
            if value is not None:
                totals[key] = max(totals.get(key, 0), value)
    usage = {
        key: sum(values.get(key, 0) for values in by_session.values())
        for key in usage_keys
    }
    return {
        "events": len(rows),
        "invalid_lines": invalid,
        "sessions": len(sessions),
        "turns": len(turns),
        "tool_events": sum(bool(row.get("tool_name")) for row in rows),
        "subagent_events": sum(
            "subagent" in str(row.get("hook", "")).lower()
            or bool(row.get("subagent_type"))
            for row in rows
        ),
        "usage_aggregation": "per-session-max",
        "usage": usage,
    }


GROUPINGS = ("day", "model", "provider", "task")
USAGE_KEYS = ("input_tokens", "cache_read_tokens", "output_tokens")


def parse_bound(value: Optional[str], end: bool) -> Optional[datetime.datetime]:
    """A --since/--until bound in UTC. A bare date covers its whole day."""
    if value is None:
        return None
    if re.fullmatch(r"[0-9]{4}-[0-9]{2}-[0-9]{2}", value):
        try:
            day = datetime.datetime.strptime(value, "%Y-%m-%d").replace(tzinfo=datetime.timezone.utc)
        except ValueError:
            raise SystemExit("error: artifact telemetry: not a calendar date: %s" % value)
        return day + datetime.timedelta(days=1) if end else day
    parsed = parse_timestamp(value)
    if parsed is None:
        raise SystemExit("error: artifact telemetry: expected YYYY-MM-DD or an ISO-8601 time: %s" % value)
    return parsed.astimezone(datetime.timezone.utc)


def in_range(row: dict[str, object], since, until, until_is_day: bool) -> bool:
    """Whether the row's own time falls in the range; an undated row never does."""
    stamp = parse_timestamp(row.get("ts"))
    if stamp is None:
        return False
    if since is not None and stamp < since:
        return False
    if until is not None and (stamp >= until if until_is_day else stamp > until):
        return False
    return True


def group_label(repo: str, row: dict[str, object], by: str) -> str:
    """The recorded value the row is grouped under; never a guess."""
    if by == "day":
        stamp = parse_timestamp(row.get("ts"))
        return stamp.astimezone(datetime.timezone.utc).strftime("%Y-%m-%d") if stamp else "undated"
    if by == "provider":
        value = row.get("provider")
    elif by == "task":
        value = row.get("task_id")
    else:
        metrics = artifact_model_metrics(repo, row)
        if metrics["model_attribution"] == "ambiguous":
            return "ambiguous"
        value = metrics["served_model"] or metrics["configured_model"] or row.get("selected_model")
    return value if isinstance(value, str) and value else "unrecorded"


def empty_group(label: str) -> dict:
    return {"group": label, "calls": 0, "token_reports": 0, "provider_reported_tokens": 0,
            "usage_reports": 0, "input_tokens": 0, "cache_read_tokens": 0, "output_tokens": 0,
            "cumulative_reports_excluded": 0, "cost_reports": 0, "cost_usd": 0.0}


def add_to_group(group: dict, repo: str, row: dict[str, object], tokens: Optional[int], detail: Optional[dict]) -> None:
    group["calls"] += 1
    if tokens is not None:
        group["token_reports"] += 1
        group["provider_reported_tokens"] += tokens
    if detail and detail.get("session_cumulative") is True:
        # A resumed session reports its running total: adding it to the calls
        # before it would count the same tokens twice.
        group["cumulative_reports_excluded"] += 1
    elif detail and all(integer(detail.get(key)) is not None for key in USAGE_KEYS):
        group["usage_reports"] += 1
        for key in USAGE_KEYS:
            group[key] += detail[key]
    cost = artifact_model_metrics(repo, row)["cost_usd"]
    if cost is not None:
        group["cost_reports"] += 1
        group["cost_usd"] = round(group["cost_usd"] + cost, 6)


def telemetry(repo: str, index: str, limit: int, by: Optional[str] = None,
              since: Optional[str] = None, until: Optional[str] = None) -> dict:
    rows: list[dict[str, object]] = []
    invalid_lines = 0
    if os.path.exists(index):
        with open(index, encoding="utf-8", errors="replace") as handle:
            for line in handle:
                try:
                    row = json.loads(line)
                except (TypeError, ValueError):
                    invalid_lines += 1
                    continue
                if not isinstance(row, dict):
                    invalid_lines += 1
                    continue
                rows.append(row)

    since_at, until_at = parse_bound(since, False), parse_bound(until, True)
    bounded = since_at is not None or until_at is not None
    until_is_day = until is not None and re.fullmatch(r"[0-9]{4}-[0-9]{2}-[0-9]{2}", until) is not None
    matched = [row for row in rows if in_range(row, since_at, until_at, until_is_day)] if bounded else rows
    selected = matched[-limit:]
    groups: dict[str, dict] = {}
    eligible = [
        row
        for row in selected
        if isinstance(row.get("kind"), str)
        and row.get("kind") in ELIGIBLE_KINDS
    ]
    resolved = resolved_events(rows)
    exit_counts = Counter()
    verification = Counter()
    fallback = Counter()
    fallback_reasons: Counter[str] = Counter()
    resolution = Counter()
    coverage = Counter()
    outcomes = Counter()
    token_values: list[int] = []
    prompt_byte_values: list[int] = []
    duration_values: list[float] = []
    routes: dict[tuple[str, str, str], dict] = {}

    resolution["events"] = sum(
        row.get("kind") == "artifact-resolution" for row in selected
    )
    for row in eligible:
        exit_value = integer(row.get("exit"))
        if exit_value is None:
            exit_counts["unavailable"] += 1
        elif exit_value == 0:
            exit_counts["zero"] += 1
        else:
            exit_counts["nonzero"] += 1
            event_id = row.get("event_id")
            if isinstance(event_id, str) and event_id in resolved:
                resolution["resolved_nonzero"] += 1
            else:
                resolution["unresolved_nonzero"] += 1

        verify_exit = integer(row.get("verify_exit"))
        if row.get("kind") == "delegate" and row.get("context_verification") in ("none", "dry-run"):
            verify_exit = None
        if verify_exit is None:
            verification["unavailable"] += 1
            outcomes["unknown"] += 1
        else:
            verification["recorded"] += 1
            coverage["verification_reports"] += 1
            if verify_exit == 0:
                verification["zero"] += 1
                if exit_value == 0:
                    outcomes["verified_success"] += 1
                elif exit_value is None:
                    outcomes["unknown"] += 1
                else:
                    outcomes["verified_failure"] += 1
            else:
                verification["nonzero"] += 1
                outcomes["verified_failure"] += 1

        fallback_value = row.get("fallback_used")
        if fallback_value is True:
            fallback["used"] += 1
            reason = row.get("fallback_reason")
            if isinstance(reason, str) and reason:
                fallback_reasons[reason] += 1
        elif fallback_value is False:
            fallback["not_used"] += 1
        else:
            fallback["unrecorded"] += 1

        provider = row.get("provider")
        provider = provider if isinstance(provider, str) and provider else "unrecorded"
        model_class = row.get("model_class")
        model_class = (
            model_class
            if isinstance(model_class, str) and model_class in MODEL_CLASSES
            else "unrecorded"
        )
        selected_model = row.get("selected_model")
        selected_model = (
            selected_model
            if isinstance(selected_model, str) and selected_model
            else "unrecorded"
        )
        if (
            provider != "unrecorded"
            and model_class != "unrecorded"
            and selected_model != "unrecorded"
        ):
            coverage["routing_metadata"] += 1

        route = routes.setdefault(
            (provider, model_class, selected_model),
            empty_route(provider, model_class, selected_model),
        )
        route["operations"] += 1
        if exit_value is None:
            route["recorded_exit_unavailable"] += 1
        elif exit_value == 0:
            route["recorded_exit_zero"] += 1
        else:
            route["recorded_exit_nonzero"] += 1
        if verify_exit is not None:
            route["verification_recorded"] += 1
            if verify_exit != 0:
                route["verification_nonzero"] += 1
                route["verified_failure"] += 1
            elif exit_value == 0:
                route["verified_success"] += 1
            elif exit_value is None:
                route["outcome_unknown"] += 1
            else:
                route["verified_failure"] += 1
        else:
            route["outcome_unknown"] += 1
        if fallback_value is True:
            route["fallback_used"] += 1

        detail = artifact_usage_metrics(repo, row)
        keys = ("input_tokens", "cache_read_tokens", "output_tokens")
        if detail and all(integer(detail.get(key)) is not None for key in keys):
            route["usage_detail"]["reports"] += 1
            for key in keys:
                route["usage_detail"][key] += detail[key]

        present, duration, tokens = artifact_metrics(repo, row)
        prompt_bytes = integer(row.get("prompt_bytes"))
        if prompt_bytes is not None:
            prompt_byte_values.append(prompt_bytes)
        if present:
            coverage["artifact_files"] += 1
        if duration is not None:
            coverage["duration_reports"] += 1
            duration_values.append(duration)
            route["duration_reports"] += 1
            route["wall_seconds"] = round(route["wall_seconds"] + duration, 3)
        if tokens is not None:
            coverage["token_reports"] += 1
            token_values.append(tokens)
            route["token_reports"] += 1
            route["provider_reported_tokens"] += tokens
        if by:
            label = group_label(repo, row, by)
            add_to_group(groups.setdefault(label, empty_group(label)), repo, row, tokens, detail)

    first_ts = selected[0].get("ts", "") if selected else ""
    last_ts = selected[-1].get("ts", "") if selected else ""
    report = {
        "schema": 1,
        "action": "telemetry",
        "semantic_outcome": (
            "mechanical-only" if verification["recorded"] else "unavailable"
        ),
        "window": {
            "scope": "retained-index-window",
            "available_rows": len(rows),
            "selected_rows": len(selected),
            "limit": limit,
            "invalid_lines": invalid_lines,
            "first_ts": first_ts,
            "last_ts": last_ts,
        },
        "operations": {
            "eligible": len(eligible),
            "excluded": len(selected) - len(eligible),
        },
        "recorded_exit": {
            "zero": exit_counts["zero"],
            "nonzero": exit_counts["nonzero"],
            "unavailable": exit_counts["unavailable"],
        },
        "verification": {
            "recorded": verification["recorded"],
            "zero": verification["zero"],
            "nonzero": verification["nonzero"],
            "unavailable": verification["unavailable"],
        },
        "outcomes": {
            "verified_success": outcomes["verified_success"],
            "verified_failure": outcomes["verified_failure"],
            "unknown": outcomes["unknown"],
        },
        "fallback": {
            "used": fallback["used"],
            "not_used": fallback["not_used"],
            "unrecorded": fallback["unrecorded"],
            "reasons": [
                {"reason": reason, "count": count}
                for reason, count in sorted(fallback_reasons.items())
            ],
        },
        "resolution": {
            "events": resolution["events"],
            "resolved_nonzero": resolution["resolved_nonzero"],
            "unresolved_nonzero": resolution["unresolved_nonzero"],
        },
        "coverage": {
            "routing_metadata": coverage["routing_metadata"],
            "artifact_files": coverage["artifact_files"],
            "duration_reports": coverage["duration_reports"],
            "token_reports": coverage["token_reports"],
            "verification_reports": coverage["verification_reports"],
        },
        "usage": {
            "prompt_bytes": {
                "reports": len(prompt_byte_values),
                "total": sum(prompt_byte_values),
                "basis": "recorded primary prompt only; excludes provider-added context, retries and repair prompts; not tokens",
            },
            "provider_reported_tokens": {
                "reports": len(token_values),
                "total": sum(token_values),
            },
            "wall_seconds": {
                "reports": len(duration_values),
                "total": round(sum(duration_values), 3),
                "median": (
                    round(float(statistics.median(duration_values)), 3)
                    if duration_values
                    else None
                ),
            },
        },
        "routes": [routes[key] for key in sorted(routes)],
        "native_activity": native_activity(repo),
    }
    if bounded:
        # Only a bounded run reports these: an unbounded report keeps its shape.
        report["window"].update({
            "since": since, "until": until, "timezone": "UTC", "matched_rows": len(matched),
            "undated_rows": sum(parse_timestamp(row.get("ts")) is None for row in rows),
        })
    if by:
        for group in groups.values():
            if not group["cost_reports"]:
                group["cost_usd"] = None  # unreported is not free
        report["usage_by"] = {
            "by": by,
            "timezone": "UTC",
            "basis": "provider-reported values only; nothing is priced or estimated from tokens",
            # More rows matched than the limit keeps: the oldest are not in the groups.
            "capped": len(matched) > len(selected),
            "omitted_rows": len(matched) - len(selected),
            "groups": [groups[key] for key in sorted(groups)],
        }
    from repair_replay import repeated, valid_trace
    delegates = [row for row in selected if row.get("kind") == "delegate"]
    traces = [row["repair_replay"] for row in delegates if valid_trace(row.get("repair_replay"))]
    if traces:
        report["repair_replay"] = {
            "recorded": len(traces),
            "delegations_seen": len(delegates),
            "selected": sum(trace["policy"] == "stop-repeat" for trace in traces),
            "policy_counts": dict(Counter(trace["policy"] for trace in traces)),
            "early_stops": sum(trace["policy"] == "stop-repeat" and repeated(trace["steps"])
                               and len(trace["steps"]) < trace["budget"] + 1 for trace in traces),
            "basis": "historical-prefix replay; not proof of future success",
            "cost_basis": "worker attempts only; not measured token or wall-clock savings",
        }
    return report


def emit_human(report: dict) -> None:
    if "repair_replay" in report:
        block = report["repair_replay"]
        print("repair replay: %d/%d delegations recorded, %d selected, %d early stops (token savings unmeasured)"
              % (block["recorded"], block["delegations_seen"], block["selected"], block["early_stops"]))
    window = report["window"]
    operations = report["operations"]
    exits = report["recorded_exit"]
    verification = report["verification"]
    fallback = report["fallback"]
    coverage = report["coverage"]
    usage = report["usage"]
    print(
        "retained window: %d/%d row(s) (limit %d; invalid lines %d)"
        % (
            window["selected_rows"],
            window["available_rows"],
            window["limit"],
            window["invalid_lines"],
        )
    )
    print(
        "routed operations: %d (excluded %d)"
        % (operations["eligible"], operations["excluded"])
    )
    print("semantic outcome: %s" % report["semantic_outcome"])
    print(
        "recorded exit: zero=%d nonzero=%d unavailable=%d"
        % (exits["zero"], exits["nonzero"], exits["unavailable"])
    )
    print(
        "verification: recorded=%d nonzero=%d unavailable=%d"
        % (
            verification["recorded"],
            verification["nonzero"],
            verification["unavailable"],
        )
    )
    print(
        "fallback: used=%d not-used=%d unrecorded=%d"
        % (fallback["used"], fallback["not_used"], fallback["unrecorded"])
    )
    print(
        "coverage: routing=%d artifacts=%d durations=%d token-reports=%d"
        % (
            coverage["routing_metadata"],
            coverage["artifact_files"],
            coverage["duration_reports"],
            coverage["token_reports"],
        )
    )
    print(
        "provider-reported tokens: total=%d reports=%d; "
        "observed wall seconds: total=%s median=%s reports=%d"
        % (
            usage["provider_reported_tokens"]["total"],
            usage["provider_reported_tokens"]["reports"],
            usage["wall_seconds"]["total"],
            usage["wall_seconds"]["median"],
            usage["wall_seconds"]["reports"],
        )
    )
    prompt_bytes = usage["prompt_bytes"]
    print("prompt bytes: total=%d reports=%d (%s)" % (
        prompt_bytes["total"], prompt_bytes["reports"], prompt_bytes["basis"]))
    native = report["native_activity"]
    print(
        "native activity: sessions=%d turns=%d tools=%d subagents=%d events=%d"
        % (
            native["sessions"],
            native["turns"],
            native["tool_events"],
            native["subagent_events"],
            native["events"],
        )
    )
    for route in report["routes"]:
        detail = route["usage_detail"]
        if detail["reports"]:
            print("tokens: provider=%s model=%s input=%d cached=%d output=%d reports=%d" % (
                route["provider"], route["selected_model"], detail["input_tokens"],
                detail["cache_read_tokens"], detail["output_tokens"], detail["reports"]))
        print(
            "route: provider=%s class=%s model=%s operations=%d "
            "exit-zero=%d exit-nonzero=%d fallback=%d"
            % (
                route["provider"],
                route["model_class"],
                route["selected_model"],
                route["operations"],
                route["recorded_exit_zero"],
                route["recorded_exit_nonzero"],
                route["fallback_used"],
            )
        )


def reported(total, reports: int, calls: int) -> str:
    """`-` when no call reported the value, `N+?` when only some did."""
    if not reports:
        return "-"
    text = ("%.4f" % total) if isinstance(total, float) else str(total)
    return text if reports == calls else text + "+?"


def emit_usage_by(report: dict) -> None:
    window, block = report["window"], report["usage_by"]
    scope = "limit %d" % window["limit"]
    if "matched_rows" in window:
        scope = "%d in range; %s" % (window["matched_rows"], scope)
    print("usage by %s%s: %d routed operation(s) in %d/%d retained row(s) (%s)" % (
        block["by"], " (UTC)" if block["by"] == "day" else "", report["operations"]["eligible"],
        window["selected_rows"], window["available_rows"], scope))
    if block["capped"]:
        print("window capped: the %d oldest matching row(s) are not counted; raise N or narrow the range"
              % block["omitted_rows"])
    else:
        print("window complete: every matching retained row is counted (pruned history is not)")
    if window.get("undated_rows"):
        print("undated: %d row(s) carry no time and are outside any range" % window["undated_rows"])
    head = (block["by"], "calls", "tokens", "input", "cached", "output", "cost-usd")
    table = [head] + [(
        group["group"], str(group["calls"]),
        reported(group["provider_reported_tokens"], group["token_reports"], group["calls"]),
        reported(group["input_tokens"], group["usage_reports"], group["calls"]),
        reported(group["cache_read_tokens"], group["usage_reports"], group["calls"]),
        reported(group["output_tokens"], group["usage_reports"], group["calls"]),
        reported(group["cost_usd"], group["cost_reports"], group["calls"]),
    ) for group in block["groups"]]
    widths = [max(len(line[column]) for line in table) for column in range(len(head))]
    for line in table:
        print("  ".join([line[0].ljust(widths[0])] + [cell.rjust(widths[n]) for n, cell in enumerate(line) if n]).rstrip())
    if not block["groups"]:
        print("(no routed operation in this window)")
    excluded = sum(group["cumulative_reports_excluded"] for group in block["groups"])
    if excluded:
        print("cumulative: %d resumed-session report(s) carry a running total and are left out of "
              "input/cached/output" % excluded)
    print("reported values only: `-` none reported, `+?` some calls reported nothing; "
          "no cost is derived from tokens and groups are not summed")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo", required=True)
    parser.add_argument("--index")
    parser.add_argument("--limit", type=int)
    parser.add_argument("--json", action="store_true")
    parser.add_argument("--by", choices=GROUPINGS)
    parser.add_argument("--since")
    parser.add_argument("--until")
    # Extract mode exists so the index writer can cache what only this module
    # knows how to read, without a second copy of the regexes drifting from it.
    parser.add_argument("--extract", help="artifact path, repo-relative")
    parser.add_argument("--ts", help="row timestamp, for the duration")
    args = parser.parse_args()

    if args.extract:
        repo = os.path.realpath(args.repo)
        row = {"artifact": args.extract, "ts": args.ts}
        _, duration, tokens = artifact_metrics(repo, row)
        out: dict[str, object] = {}
        if duration is not None:
            out["duration_s"] = duration
        if tokens is not None:
            out["tokens"] = tokens
        print(json.dumps(out, sort_keys=True))
        return 0

    if not args.index or args.limit is None:
        parser.error("--index and --limit are required without --extract")
    if args.limit <= 0:
        parser.error("--limit must be positive")
    repo = os.path.realpath(args.repo)
    report = telemetry(repo, args.index, args.limit, args.by, args.since, args.until)
    if args.json:
        print(json.dumps(report, ensure_ascii=False, sort_keys=True))
    elif args.by:
        emit_usage_by(report)
    else:
        emit_human(report)
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except OSError as error:
        sys.stderr.write("error: artifact telemetry: %s\n" % error)
        raise SystemExit(2)
