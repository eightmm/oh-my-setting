#!/usr/bin/env python3
"""Summarize a metrics file for `oms job-digest --metrics FILE`.

Reads either a JSONL file (one object per line: step, name, value, ts) or
DVCLive `plots/metrics` TSV files (a single .tsv, or a directory of them with
columns timestamp, step, <name>). Prints a markdown section: latest step per
metric, latest and best value, recent slope, non-finite values and the time
since the last record. Stateless and read-only; stdlib only. The age is an
observation of the file, never evidence that a run is alive, dead or correct.
"""
import argparse
import collections
import datetime
import json
import math
import os
import sys

RECENT = 10
LOWER_IS_BETTER = ("loss", "err", "perplex", "ppl", "mae", "mse", "rmse", "nll")


def parse_ts(value):
    """Epoch seconds from a number, numeric string or ISO-8601 text, else None."""
    if isinstance(value, bool) or value is None:
        return None
    if isinstance(value, str):
        text = value.strip()
        try:
            value = float(text)
        except ValueError:
            try:
                parsed = datetime.datetime.fromisoformat(text.replace("Z", "+00:00"))
            except ValueError:
                return None
            if parsed.tzinfo is None:
                parsed = parsed.replace(tzinfo=datetime.timezone.utc)
            return parsed.timestamp()
    if isinstance(value, (int, float)) and math.isfinite(value):
        return value / 1000.0 if value > 1e11 else float(value)
    return None


def parse_number(value):
    """(float, ok). Strings such as "nan" and "inf" are non-finite numbers."""
    if isinstance(value, bool) or value is None:
        return None, False
    if isinstance(value, (int, float)):
        return float(value), True
    if isinstance(value, str):
        try:
            return float(value.strip()), True
        except ValueError:
            return None, False
    return None, False


class Series:
    def __init__(self, name):
        self.name = name
        self.count = 0
        self.recent = collections.deque(maxlen=RECENT)
        self.finite = 0
        self.nonfinite = 0
        self.first_nonfinite = None
        self.min = None
        self.max = None
        self.last_ts = None

    def add(self, step, value, ts):
        self.count += 1
        if ts is not None and (self.last_ts is None or ts > self.last_ts):
            self.last_ts = ts
        if not math.isfinite(value):
            self.nonfinite += 1
            if self.first_nonfinite is None:
                self.first_nonfinite = step
            self.recent.append((step, value))
            return
        self.finite += 1
        if self.min is None or value < self.min[1]:
            self.min = (step, value)
        if self.max is None or value > self.max[1]:
            self.max = (step, value)
        self.recent.append((step, value))

    def lower_is_better(self):
        low = self.name.lower()
        return any(word in low for word in LOWER_IS_BETTER)

    def slope(self):
        points = [(s, v) for s, v in self.recent if math.isfinite(v)]
        if len(points) < 3:
            return None
        xs = [float(s) for s, _ in points]
        if max(xs) == min(xs):
            return None
        mx = sum(xs) / len(xs)
        my = sum(v for _, v in points) / len(points)
        den = sum((x - mx) ** 2 for x in xs)
        return sum((x - mx) * (v - my) for x, (_, v) in zip(xs, points)) / den


def read_jsonl(path, series, stats):
    with open(path, encoding="utf-8", errors="replace") as handle:
        for line in handle:
            line = line.strip()
            if not line:
                continue
            try:
                row = json.loads(line)
            except ValueError:
                stats["skipped"] += 1
                continue
            if not isinstance(row, dict):
                stats["skipped"] += 1
                continue
            name = row.get("name")
            value, ok = parse_number(row.get("value"))
            step, step_ok = parse_number(row.get("step"))
            if not isinstance(name, str) or not name or not ok or not step_ok:
                stats["skipped"] += 1
                continue
            series.setdefault(name, Series(name)).add(int(step), value, parse_ts(row.get("ts")))


def read_tsv(path, series, stats):
    default = os.path.splitext(os.path.basename(path))[0]
    with open(path, encoding="utf-8", errors="replace") as handle:
        header = handle.readline().rstrip("\n").split("\t")
        if "step" not in header:
            stats["skipped"] += 1
            return
        step_i = header.index("step")
        ts_i = header.index("timestamp") if "timestamp" in header else None
        cols = [(i, n) for i, n in enumerate(header) if n not in ("step", "timestamp")]
        if len(cols) == 1:
            cols = [(cols[0][0], default if cols[0][1] in ("", "value") else cols[0][1])]
        for line in handle:
            cells = line.rstrip("\n").split("\t")
            if len(cells) != len(header):
                if line.strip():
                    stats["skipped"] += 1
                continue
            step, step_ok = parse_number(cells[step_i])
            if not step_ok:
                stats["skipped"] += 1
                continue
            ts = parse_ts(cells[ts_i]) if ts_i is not None else None
            for index, name in cols:
                value, ok = parse_number(cells[index])
                if ok:
                    series.setdefault(name, Series(name)).add(int(step), value, ts)
                else:
                    stats["skipped"] += 1


def gather(target):
    series = {}
    stats = {"skipped": 0, "files": 0}
    if os.path.isdir(target):
        paths = []
        for root, _dirs, files in os.walk(target):
            paths.extend(os.path.join(root, f) for f in files if f.endswith(".tsv"))
        paths.sort()
    else:
        paths = [target]
    for path in paths:
        stats["files"] += 1
        if path.endswith(".tsv"):
            read_tsv(path, series, stats)
        else:
            read_jsonl(path, series, stats)
    return series, stats


def human_age(seconds):
    if seconds < 0:
        return "in the future (clock skew?)"
    seconds = int(seconds)
    if seconds < 90:
        return "%ds ago" % seconds
    if seconds < 5400:
        return "%dm ago" % (seconds // 60)
    if seconds < 172800:
        return "%.1fh ago" % (seconds / 3600.0)
    return "%.1fd ago" % (seconds / 86400.0)


def fmt(value):
    if not math.isfinite(value):
        return str(value)
    return "%.6g" % value


def main(argv):
    parser = argparse.ArgumentParser()
    parser.add_argument("target")
    parser.add_argument("--max-series", type=int, default=20)
    args = parser.parse_args(argv)
    if not os.path.exists(args.target):
        print("error: metrics file not found: %s" % args.target, file=sys.stderr)
        return 2
    series, stats = gather(args.target)
    print("## Metrics\n")
    print("- source: %s (%d file(s), %d record(s) skipped as malformed)"
          % (args.target, stats["files"], stats["skipped"]))
    if not series:
        print("\nNo metric records found: not evidence that the run made no progress.")
        return 0
    last_ts = max((s.last_ts for s in series.values() if s.last_ts is not None), default=None)
    if last_ts is None:
        print("- last record: no timestamp in the file; staleness cannot be read")
    else:
        age = datetime.datetime.now(datetime.timezone.utc).timestamp() - last_ts
        print("- last record: %s (file observation only; not proof the run is alive or dead)"
              % human_age(age))
    names = sorted(series)
    shown = names[: max(args.max_series, 1)]
    print("\n| metric | records | last step | latest | best (rule) | slope/step (last %d) | non-finite |"
          % RECENT)
    print("|---|---|---|---|---|---|---|")
    for name in shown:
        item = series[name]
        last_step, latest = item.recent[-1]
        best, rule = (item.min, "min") if item.lower_is_better() else (item.max, "max")
        best_text = "%s @%d (%s)" % (fmt(best[1]), best[0], rule) if best else "n/a"
        slope = item.slope()
        slope_text = "%+.3g" % slope if slope is not None else "n/a"
        nonfinite = ("%d, first @%d" % (item.nonfinite, item.first_nonfinite)
                     if item.nonfinite else "0")
        print("| %s | %d | %d | %s | %s | %s | %s |"
              % (name, item.count, last_step, fmt(latest), best_text, slope_text, nonfinite))
    if len(names) > len(shown):
        print("\n%d more metric(s) not shown." % (len(names) - len(shown)))
    flagged = [n for n in names if series[n].nonfinite]
    if flagged:
        print("\nNon-finite values recorded in: %s." % ", ".join(flagged))
    print("\nBest uses min for names containing loss/err/ppl/mae/mse/nll, otherwise max;"
          " check the direction for your metric.")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
