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
MODEL_CLASSES = frozenset(("fast", "balanced", "deep"))
STARTED_RE = re.compile(r"(?m)^- started:\s*(\S+)\s*$")
TOKENS_RE = re.compile(
    r"(?im)^tokens used[ \t]*\r?\n[ \t]*([0-9][0-9,]*)[ \t]*$"
)
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

    artifact = row.get("artifact")
    if not isinstance(artifact, str) or not artifact or os.path.isabs(artifact):
        return False, saved_duration, saved_tokens
    candidate = os.path.realpath(os.path.join(repo, artifact))
    if not inside(candidate, repo) or not os.path.isfile(candidate):
        return False, saved_duration, saved_tokens
    try:
        with open(candidate, "rb") as handle:
            size = os.fstat(handle.fileno()).st_size
            if size <= ARTIFACT_EDGE_BYTES * 2:
                raw = handle.read()
            else:
                head = handle.read(ARTIFACT_EDGE_BYTES)
                handle.seek(-ARTIFACT_EDGE_BYTES, os.SEEK_END)
                raw = head + b"\n" + handle.read(ARTIFACT_EDGE_BYTES)
            text = raw.decode("utf-8", errors="replace")
    except OSError:
        return False, saved_duration, saved_tokens

    token_matches = TOKENS_RE.findall(text)
    tokens = int(token_matches[-1].replace(",", "")) if token_matches else None
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
        "duration_reports": 0,
        "wall_seconds": 0.0,
    }


def telemetry(repo: str, index: str, limit: int) -> dict:
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

    selected = rows[-limit:]
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
    token_values: list[int] = []
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
        if verify_exit is None:
            verification["unavailable"] += 1
        else:
            verification["recorded"] += 1
            if verify_exit == 0:
                verification["zero"] += 1
            else:
                verification["nonzero"] += 1

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
        if fallback_value is True:
            route["fallback_used"] += 1

        present, duration, tokens = artifact_metrics(repo, row)
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

    first_ts = selected[0].get("ts", "") if selected else ""
    last_ts = selected[-1].get("ts", "") if selected else ""
    return {
        "schema": 1,
        "action": "telemetry",
        "semantic_outcome": "unavailable",
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
        },
        "usage": {
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
    }


def emit_human(report: dict) -> None:
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
    print("semantic outcome: unavailable")
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
    for route in report["routes"]:
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


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo", required=True)
    parser.add_argument("--index")
    parser.add_argument("--limit", type=int)
    parser.add_argument("--json", action="store_true")
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
    report = telemetry(repo, args.index, args.limit)
    if args.json:
        print(json.dumps(report, ensure_ascii=False, sort_keys=True))
    else:
        emit_human(report)
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except OSError as error:
        sys.stderr.write("error: artifact telemetry: %s\n" % error)
        raise SystemExit(2)
