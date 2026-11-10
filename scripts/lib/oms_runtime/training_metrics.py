"""Bounded training-metric observations, not scheduler state or a success gate."""
from __future__ import annotations

import argparse
from collections import Counter, OrderedDict
import csv
from datetime import datetime, timezone
import json
import math
from pathlib import Path
import re
import sys

from .inspection_io import ReadError, read_segments

MAX_BYTES = 4 * 1024 * 1024
MAX_LINE_BYTES = 65536
MAX_ROWS = 50000
MAX_SERIES = 64
MAX_POINTS = 2048
NAME = re.compile(r"[A-Za-z0-9][A-Za-z0-9._/:+-]{0,159}\Z")
RUN = re.compile(r"[A-Za-z0-9][A-Za-z0-9._:-]{0,119}\Z")
NONFINITE = {"nan": "nan", "inf": "positive-infinity", "+inf": "positive-infinity",
             "infinity": "positive-infinity", "+infinity": "positive-infinity",
             "-inf": "negative-infinity", "-infinity": "negative-infinity"}


class MetricsError(ValueError):
    """Malformed input, with no raw record or private path in the message."""


def _number(value):
    if isinstance(value, str):
        state = NONFINITE.get(value.lower())
        if state:
            return None, state
        raise MetricsError("value must be numeric or a named non-finite value")
    if type(value) not in (int, float):
        raise MetricsError("value must be numeric")
    try:
        number = float(value)
    except OverflowError:
        raise MetricsError("metric magnitude is excessive") from None
    if not math.isfinite(number):
        return None, "nan" if math.isnan(number) else "positive-infinity" if number > 0 else "negative-infinity"
    if abs(number) > 1e300:
        raise MetricsError("metric magnitude is excessive")
    return number, "finite"


def _timestamp(value, *, milliseconds=False):
    if value is None or value == "":
        return None
    try:
        if type(value) in (int, float):
            result = float(value) / (1000 if milliseconds else 1)
        elif isinstance(value, str) and not milliseconds:
            parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
            if parsed.utcoffset() is None:
                return None
            result = parsed.timestamp()
        else:
            return None
        return result if math.isfinite(result) and 0 <= result <= 253402300799 else None
    except (ValueError, OverflowError, OSError):
        return None


def _unique(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise MetricsError("duplicate metric record key")
        result[key] = value
    return result


def _record(row, *, milliseconds=False):
    if not isinstance(row, dict):
        raise MetricsError("metric record must be an object")
    step, name, run = row.get("step"), row.get("name"), row.get("run_id")
    if type(step) is not int or not 0 <= step <= (1 << 53) - 1:
        raise MetricsError("metric step must be a bounded nonnegative integer")
    if not isinstance(name, str) or not NAME.fullmatch(name):
        raise MetricsError("invalid metric name")
    if run is not None and (not isinstance(run, str) or not RUN.fullmatch(run)):
        raise MetricsError("invalid recorded run id")
    value, state = _number(row.get("value"))
    ts = _timestamp(row.get("ts"), milliseconds=milliseconds)
    return (run, name), {"step": step, "value": value, "value_state": state,
                         "timestamp": ts, "timestamp_unreadable": row.get("ts") not in (None, "") and ts is None}


def _freshness(point, now, stale_after, chronology_ok):
    ts = point["timestamp"]
    if ts is None or not chronology_ok:
        return {"state": "unknown", "age_seconds": None}
    age = now - ts
    if age < 0:
        return {"state": "clock-skew", "age_seconds": None}
    return {"state": "late-record" if age >= stale_after else "recent-record", "age_seconds": age}


def _summarize(key, series, now, stale_after, direction, window):
    points = list(series["points"].values())
    latest = points[-1]
    finite = [p for p in points if p["value_state"] == "finite"]
    recent = points[-window:]
    trend = {"samples": len(recent), "value_per_step": None, "reason": "insufficient-distinct-steps"}
    if len(recent) >= 2:
        if any(p["value_state"] != "finite" for p in recent):
            trend["reason"] = "non-finite-in-window"
        else:
            trend.update(value_per_step=(recent[-1]["value"]-recent[0]["value"])/(recent[-1]["step"]-recent[0]["step"]),
                         reason="endpoint-change-per-step")
    low = min(finite, key=lambda p: p["value"]) if finite else None
    high = max(finite, key=lambda p: p["value"]) if finite else None
    best = low if direction == "minimize" else high if direction == "maximize" else None
    def selected(point):
        return {"step": point["step"], "value": point["value"]} if point is not None else None
    return {"name": key[1], "run_id": key[0], "latest": latest,
            "observed_rows": series["rows"], "duplicate_steps": series["duplicates"],
            "step_regressions": series["regressions"], "timestamp_regressions": series["time_regressions"],
            "nonfinite_observations": series["nonfinite"], "retained_points": len(points),
            "omitted_earlier_points": series["omitted"], "earlier_segment_points": series["reset_points"],
            "minimum": selected(low), "maximum": selected(high), "best": selected(best), "direction": direction,
            "trend": trend, "freshness": _freshness(latest, now, stale_after, series["time_regressions"] == 0),
            "summary_scope": "retained-points-in-last-observed-monotone-step-segment"}


def inspect_metrics(repo, path, *, input_format="jsonl", metric=None, direction="observe",
                    stale_after=600, window=20, now=None):
    """Read one explicit history. Restarts and missing prefixes never imply success."""
    if input_format not in ("jsonl", "dvclive") or direction not in ("observe", "minimize", "maximize"):
        raise MetricsError("unsupported metrics format or direction")
    if metric is not None and (not isinstance(metric, str) or not NAME.fullmatch(metric)):
        raise MetricsError("invalid selected metric")
    if direction != "observe" and metric is None:
        raise MetricsError("a direction requires an exact --metric")
    if type(stale_after) is not int or not 1 <= stale_after <= 604800 or type(window) is not int or not 2 <= window <= 200:
        raise MetricsError("invalid staleness threshold or trend window")
    now = datetime.now(timezone.utc).timestamp() if now is None else now
    if type(now) not in (int, float) or _timestamp(now) is None:
        raise MetricsError("invalid observation clock")
    read = read_segments(repo, path, max_bytes=MAX_BYTES, allow_partial=True)
    # For a large history summarize only its contiguous suffix, never bridge an unread gap.
    offset, data = read["segments"][-1]
    ignored_prefix = offset
    if offset:
        cut = data.find(b"\n")
        cut = len(data) if cut < 0 else cut + 1
        offset += cut; data = data[cut:]; ignored_prefix = offset
    header = None
    if input_format == "dvclive":
        first = read["segments"][0][1].split(b"\n", 1)[0]
        try:
            if len(first) > MAX_LINE_BYTES:
                raise MetricsError("DVCLive header exceeds the line limit")
            header = next(csv.reader([first.decode("utf-8").rstrip("\r")], delimiter="\t", strict=True))
        except (UnicodeError, csv.Error, StopIteration):
            raise MetricsError("invalid DVCLive TSV header") from None
        names = [name for name in header if name not in ("step", "timestamp")]
        if (len(set(header)) != len(header) or "step" not in header or len(names) != 1
                or len(header) not in (2, 3) or not NAME.fullmatch(names[0])):
            raise MetricsError("DVCLive TSV needs step, one metric and optional timestamp")
        if offset == 0:
            cut = data.find(b"\n")
            if cut < 0:
                raise MetricsError("incomplete DVCLive header")
            offset = cut + 1; data = data[offset:]
    diagnostics = Counter(); series = {}; seen = matched = 0; parsed_bytes = 0
    complete_end = data.rfind(b"\n") + 1
    trailing = len(data) - complete_end
    # A final unterminated row may still be in flight. Never call it a valid observation.
    data = data[:complete_end]
    while parsed_bytes < len(data):
        if seen >= MAX_ROWS:
            diagnostics["row-budget-exhausted"] += 1
            break
        end = data.find(b"\n", parsed_bytes) + 1
        raw = data[parsed_bytes:end]
        seen += 1; parsed_bytes = end
        if not raw.strip():
            diagnostics["blank-row"] += 1; continue
        if len(raw) > MAX_LINE_BYTES:
            diagnostics["oversize-row"] += 1; continue
        try:
            if input_format == "jsonl":
                row = json.loads(raw.decode("utf-8"), object_pairs_hook=_unique)
            else:
                values = next(csv.reader([raw.decode("utf-8").rstrip("\r\n")], delimiter="\t", strict=True))
                if len(values) != len(header):
                    raise MetricsError("incorrect TSV column count")
                cols = dict(zip(header, values))
                step = cols["step"]
                if not re.fullmatch(r"[0-9]{1,16}", step):
                    raise MetricsError("invalid TSV step")
                name = next(n for n in header if n not in ("step", "timestamp"))
                row = {"step": int(step), "name": name, "value": float(cols[name]),
                       "ts": float(cols["timestamp"]) if cols.get("timestamp") else None}
            key, point = _record(row, milliseconds=input_format == "dvclive")
        except (ValueError, UnicodeError, RecursionError, OverflowError, csv.Error, StopIteration):
            diagnostics["invalid-row"] += 1; continue
        if metric is not None and key[1] != metric:
            diagnostics["filtered-row"] += 1; continue
        if key not in series:
            if len(series) >= MAX_SERIES:
                diagnostics["series-budget-exhausted-rows"] += 1; continue
            series[key] = {"points": OrderedDict(), "rows": 0, "duplicates": 0, "regressions": 0,
                           "time_regressions": 0, "nonfinite": 0, "omitted": 0, "reset_points": 0}
        current = series[key]; points = current["points"]
        if points and point["step"] < next(reversed(points)):
            current["regressions"] += 1; current["reset_points"] += len(points)
            points.clear()
            # Step reversal may be a restart or out-of-order data, not a proven new run.
        if points:
            previous = next(reversed(points.values()))
            if point["timestamp"] is not None and previous["timestamp"] is not None and point["timestamp"] < previous["timestamp"]:
                current["time_regressions"] += 1
        if point["step"] in points:
            current["duplicates"] += 1
        points[point["step"]] = point
        if len(points) > MAX_POINTS:
            points.popitem(last=False); current["omitted"] += 1
        current["rows"] += 1; matched += 1
        current["nonfinite"] += point["value_state"] != "finite"
        diagnostics["invalid-timestamp"] += bool(point["timestamp_unreadable"])
    result = [_summarize(key, value, now, stale_after, direction, window) for key, value in series.items()]
    result.sort(key=lambda row: (row["name"], row["run_id"] or ""))
    gaps = (ignored_prefix or trailing or diagnostics["invalid-row"] or diagnostics["oversize-row"]
            or diagnostics["row-budget-exhausted"] or diagnostics["series-budget-exhausted-rows"]
            or any(row["omitted_earlier_points"] or row["earlier_segment_points"] for row in result))
    return {"schema": 1, "kind": "oms-training-metrics", "authority": "observation-only",
            "source": {"path": read["path"], "bytes": read["total_bytes"], "sha256": read["sha256"]},
            "coverage": {"read_bytes": read["read_bytes"], "unread_bytes": read["total_bytes"]-read["read_bytes"],
                         "ignored_prefix_bytes": ignored_prefix, "unterminated_tail_bytes": trailing,
                         "unprocessed_window_bytes": len(data)-parsed_bytes, "rows_seen": seen,
                         "matched_rows": matched, "history_complete": not bool(gaps),
                         "diagnostics": {k: v for k, v in sorted(diagnostics.items()) if v}},
            "series": result, "selected_metric": metric, "stale_after_seconds": stale_after, "trend_window": window,
            "note": "Recorded metrics only. Latest means latest processed record, not current job state. Best/min/max refer to retained points in the final observed monotone segment, not the full run. A step reversal is ambiguous; missing timestamps stay unknown. Late records do not prove a stalled job. No command, scheduler, checkpoint, retry or model is run."}


def render(report):
    def value(number):
        return "unknown" if number is None else format(number, ".6g")
    lines = ["## Recorded training metrics", "", report["note"], ""]
    coverage = report["coverage"]
    lines.append(f"Observed {coverage['matched_rows']} metric row(s); history_complete={str(coverage['history_complete']).lower()}.")
    for row in report["series"]:
        latest = row["latest"]
        lines.append(f"- {row['name']} / run={row['run_id'] or 'unrecorded'}: step={latest['step']}, value={value(latest['value'])} [{latest['value_state']}]; {row['freshness']['state']}")
        lines.append(f"  retained={row['retained_points']}, nonfinite observations={row['nonfinite_observations']}, step reversals={row['step_regressions']}, duplicate steps={row['duplicate_steps']}; trend/step={value(row['trend']['value_per_step'])}")
        if row["best"] is not None:
            lines.append(f"  best ({row['direction']}, retained segment only)={value(row['best']['value'])} at step {row['best']['step']}")
    if not report["series"]:
        lines.append("No usable selected observations; this is not a success verdict.")
    lines.append("Coverage: " + json.dumps(coverage, ensure_ascii=True, sort_keys=True))
    return "\n".join(lines)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("path")
    parser.add_argument("--repo", default=".")
    parser.add_argument("--input-format", choices=("jsonl", "dvclive"), default="jsonl")
    parser.add_argument("--metric")
    parser.add_argument("--direction", choices=("observe", "minimize", "maximize"), default="observe")
    parser.add_argument("--stale-after", type=int, default=600)
    parser.add_argument("--window", type=int, default=20)
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args(argv)
    try:
        report = inspect_metrics(Path(args.repo), args.path, input_format=args.input_format,
                                 metric=args.metric, direction=args.direction, stale_after=args.stale_after, window=args.window)
    except (MetricsError, ReadError) as exc:
        print("metrics: " + str(exc), file=sys.stderr)
        return 2
    print(json.dumps(report, ensure_ascii=False, allow_nan=False, sort_keys=True) if args.json else render(report))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
