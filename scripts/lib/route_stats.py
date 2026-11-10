"""Outcome history per role x provider x model x effort, derived from recorded write attempts.

Read-only. The inputs are rows panel_metrics.aggregate already derived from the lifecycle
stream and the owner decisions; nothing is stored here and nothing is applied. A `suggest`
line is reported history for a person to weigh, never a routing decision.
"""

import statistics

DECIDED = ("accepted", "completed", "rejected", "failed")
DEFAULT_MIN_SAMPLES = 3


def _clean(value, fallback):
    return value if isinstance(value, str) and value else fallback


def route_history(rows, reworked, min_samples=DEFAULT_MIN_SAMPLES):
    """rows: dicts with role, provider, model, effort, task, bucket, tokens (int or None)."""
    groups = {}
    for row in rows:
        key = (_clean(row.get("role"), "unknown"), _clean(row.get("provider"), "unknown"),
               _clean(row.get("model"), "unknown"), _clean(row.get("effort"), "unrecorded"))
        entry = groups.setdefault(key, {"attempts": 0, "tokens": [], "tasks": {}})
        entry["attempts"] += 1
        if isinstance(row.get("tokens"), int) and not isinstance(row.get("tokens"), bool):
            entry["tokens"].append(row["tokens"])
        entry["tasks"].setdefault(row["task"], row["bucket"])
    shown, hidden = [], 0
    for (role, provider, model, effort), entry in sorted(groups.items()):
        buckets = list(entry["tasks"].values())
        decided = sum(bucket in DECIDED for bucket in buckets)
        admitted = buckets.count("accepted")
        if decided < min_samples:
            hidden += 1
            continue
        shown.append({
            "role": role, "provider": provider, "model": model, "effort": effort,
            "attempts": entry["attempts"], "tasks": len(buckets), "decided": decided,
            "admitted": admitted, "rejected": buckets.count("rejected"),
            "reworked": sum(task in reworked for task in entry["tasks"]),
            "admit_rate": round(admitted / decided, 4),
            "median_tokens": statistics.median(entry["tokens"]) if entry["tokens"] else None,
            "token_reports": len(entry["tokens"]),
        })
    suggest = {}
    for item in shown:
        best = suggest.get(item["role"])
        if best is None or (item["admit_rate"], item["decided"]) > (best["admit_rate"], best["decided"]):
            suggest[item["role"]] = item
    return {"min_samples": min_samples, "unit": "tasks decided by the owner; attempts counted separately",
            "hidden_groups": hidden, "groups": shown,
            "suggest": {role: {key: item[key] for key in ("provider", "model", "effort", "admit_rate", "decided")}
                        for role, item in sorted(suggest.items())},
            "note": "reported history, not a decision; nothing is applied"}


def route_text(report):
    history = report.get("route_history") or {}
    lines = []
    for item in history.get("groups", []):
        median = item["median_tokens"]
        lines.append("route %s %s/%s effort=%s: attempts=%d admitted=%d/%d rejected=%d reworked=%d admit=%.1f%% median_tokens=%s" % (
            item["role"], item["provider"], item["model"], item["effort"], item["attempts"], item["admitted"],
            item["decided"], item["rejected"], item["reworked"], item["admit_rate"] * 100,
            "none" if median is None else "%g" % median))
    lines.append("route groups hidden under --min-samples %s: %d" % (
        history.get("min_samples", DEFAULT_MIN_SAMPLES), history.get("hidden_groups", 0)))
    for role, best in history.get("suggest", {}).items():
        lines.append("suggest %s: %s/%s effort=%s admit %.1f%% over %d decided (reported history, not a decision)" % (
            role, best["provider"], best["model"], best["effort"], best["admit_rate"] * 100, best["decided"]))
    return "\n".join(lines)
