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
    ("explicit", "provider-default", "fast", "balanced", "deep")
)
STARTED_RE = re.compile(r"(?m)^- started:\s*(\S+)\s*$")
TOKENS_RE = re.compile(
    r"(?im)^tokens used[ \t]*\r?\n[ \t]*([0-9][0-9,]*)[ \t]*$"
)
ARTIFACT_EDGE_BYTES = 64 * 1024
# A cohort this small cannot carry a rate. Below the floor the uptake report
# prints counts and withholds every ratio, because two delegations shaped into
# a percentage read as a finding and there is none yet.
REVIEW_UPTAKE_MIN_SAMPLE = 5


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


def ratio(part: int, whole: int) -> Optional[float]:
    return round(part / whole, 3) if whole else None


def review_fed(row: dict[str, object]) -> bool:
    """True when a delegation recorded the review artifact it was fed.

    The two lineage shapes differ: an in-repo review is a repo-relative path
    under `source`, an outside one is a name-only dict under `source_external`
    because the harness never owned that file.
    """
    source = row.get("source")
    if isinstance(source, str) and source:
        return True
    external = row.get("source_external")
    name = external.get("name") if isinstance(external, dict) else None
    return isinstance(name, str) and bool(name)


def source_hashed(row: dict[str, object]) -> bool:
    digest = row.get("source_sha256")
    if isinstance(digest, str) and digest:
        return True
    external = row.get("source_external")
    digest = external.get("sha256") if isinstance(external, dict) else None
    return isinstance(digest, str) and bool(digest)


def cohort(repo: str, name: str, rows: list[dict[str, object]]) -> dict:
    """Mechanical figures for one delegation cohort. Nothing is inferred."""
    exits = Counter()
    verification = Counter()
    lineage = Counter()
    durations: list[float] = []
    tokens: list[int] = []
    for row in rows:
        exit_value = integer(row.get("exit"))
        if exit_value is None:
            exits["unavailable"] += 1
        elif exit_value == 0:
            exits["zero"] += 1
        else:
            exits["nonzero"] += 1

        verify_exit = integer(row.get("verify_exit"))
        if verify_exit is not None:
            verification["recorded"] += 1
            if verify_exit == 0:
                verification["zero"] += 1

        operation_id = row.get("operation_id")
        if isinstance(operation_id, str) and operation_id:
            lineage["operation_id"] += 1
        prompt_hash = row.get("prompt_sha256")
        if isinstance(prompt_hash, str) and prompt_hash:
            lineage["prompt_hash"] += 1
        if source_hashed(row):
            lineage["source_hash"] += 1

        # Cached row metrics first, so a pruned artifact does not erase the
        # figures a previous call already reported.
        _, duration, token_count = artifact_metrics(repo, row)
        if duration is not None:
            durations.append(duration)
        if token_count is not None:
            tokens.append(token_count)

    size = len(rows)
    reportable = size >= REVIEW_UPTAKE_MIN_SAMPLE
    return {
        "cohort": name,
        "size": size,
        "status": "reported" if reportable else "insufficient-data",
        "recorded_exit": {
            "zero": exits["zero"],
            "nonzero": exits["nonzero"],
            "unavailable": exits["unavailable"],
        },
        # Named for what it counts: an exit zero is not semantic task success.
        "recorded_exit_zero_rate": (
            ratio(exits["zero"], size) if reportable else None
        ),
        "verification": {
            "recorded": verification["recorded"],
            "zero": verification["zero"],
        },
        "verifier_coverage_rate": (
            ratio(verification["recorded"], size) if reportable else None
        ),
        "verifier_exit_zero_rate": (
            ratio(verification["zero"], verification["recorded"])
            if reportable
            else None
        ),
        "lineage": {
            "operation_id": lineage["operation_id"],
            "prompt_hash": lineage["prompt_hash"],
            "source_hash": lineage["source_hash"],
        },
        "wall_seconds": {
            "reports": len(durations),
            "total": round(sum(durations), 3),
            "median": (
                round(float(statistics.median(durations)), 3)
                if reportable and durations
                else None
            ),
        },
        "provider_reported_tokens": {
            "reports": len(tokens),
            "total": sum(tokens),
        },
    }


def review_uptake(repo: str, selected: list[dict[str, object]]) -> dict:
    delegations = [row for row in selected if row.get("kind") == "delegate"]
    cohorts = [
        cohort(repo, "review-fed", [r for r in delegations if review_fed(r)]),
        cohort(
            repo, "direct", [r for r in delegations if not review_fed(r)]
        ),
    ]
    comparable = all(entry["status"] == "reported" for entry in cohorts)
    return {
        "basis": "observational, mechanical-only",
        "caveat": (
            "recorded exit zero is not semantic task success; cohort "
            "membership is not assigned, so no difference here is causal"
        ),
        "scope": "retained-index-window",
        "minimum_sample": REVIEW_UPTAKE_MIN_SAMPLE,
        "status": "reported" if comparable else "insufficient-data",
        "delegations": len(delegations),
        "cohorts": cohorts,
    }


def telemetry(
    repo: str, index: str, limit: int, uptake: bool = False
) -> dict:
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
    outcomes = Counter()
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
    # Opt-in only: omitting the flag leaves the default report byte-identical.
    if uptake:
        report["review_uptake"] = review_uptake(repo, selected)
    return report


def emit_review_uptake(block: dict) -> None:
    print(
        "review uptake: %s; status=%s; minimum sample=%d; delegations=%d"
        % (
            block["basis"],
            block["status"],
            block["minimum_sample"],
            block["delegations"],
        )
    )
    print("  caveat: %s" % block["caveat"])
    for entry in block["cohorts"]:
        exits = entry["recorded_exit"]
        verification = entry["verification"]
        lineage = entry["lineage"]
        seconds = entry["wall_seconds"]
        tokens = entry["provider_reported_tokens"]
        print(
            "  cohort %s: rows=%d status=%s recorded exit zero=%d nonzero=%d "
            "unavailable=%d verifier recorded=%d zero=%d"
            % (
                entry["cohort"],
                entry["size"],
                entry["status"],
                exits["zero"],
                exits["nonzero"],
                exits["unavailable"],
                verification["recorded"],
                verification["zero"],
            )
        )
        print(
            "    lineage: operation-id=%d prompt-hash=%d source-hash=%d; "
            "wall seconds total=%s reports=%d; tokens total=%d reports=%d"
            % (
                lineage["operation_id"],
                lineage["prompt_hash"],
                lineage["source_hash"],
                seconds["total"],
                seconds["reports"],
                tokens["total"],
                tokens["reports"],
            )
        )
        if entry["status"] != "reported":
            print("    rates withheld: cohort below the minimum sample")
            continue
        print(
            "    recorded exit-zero rate=%s verifier coverage=%s "
            "verifier exit-zero rate=%s wall seconds median=%s"
            % (
                entry["recorded_exit_zero_rate"],
                entry["verifier_coverage_rate"],
                entry["verifier_exit_zero_rate"],
                seconds["median"],
            )
        )


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
    if "review_uptake" in report:
        emit_review_uptake(report["review_uptake"])
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
    parser.add_argument("--review-uptake", action="store_true")
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
    report = telemetry(repo, args.index, args.limit, args.review_uptake)
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
