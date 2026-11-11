"""Outcome history per role x provider x model x effort, derived from recorded write attempts.

Read-only. The inputs are rows panel_metrics.aggregate already derived from the lifecycle
stream and the owner decisions; nothing is stored here and nothing is applied. A `suggest`
line is reported history for a person to weigh, never a routing decision.
"""

import os
import statistics
from pathlib import Path

DECIDED = ("accepted", "completed", "rejected", "failed")
DEFAULT_MIN_SAMPLES = 3


def _env_number(name, default, kind):
    try:
        value = kind(os.environ.get(name) or default)
    except ValueError:
        return default
    return value if 0 < value <= 1 else default


# Estimates, not measured optima. A pair is nominated when the rarer role shares a task at least
# this often; a best-to-worst admit-rate gap under the noise band is reported as no clear difference.
FOLD_CO_OCCURRENCE = _env_number("OMS_FOLD_CO_OCCURRENCE", 0.8, float)
MODEL_GAP_NOISE = _env_number("OMS_MODEL_GAP_NOISE", 0.15, float)


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


def known_roles():
    """Role names shipped with this checkout (roles/*.md); empty when the directory is absent."""
    directory = Path(__file__).resolve().parents[2] / "roles"
    return sorted(path.stem for path in directory.glob("*.md")) if directory.is_dir() else []


def _tally(rows, keyfn):
    out = {}
    for row in rows:
        entry = out.setdefault(keyfn(row), {"attempts": 0, "tokens": [], "tasks": {}})
        entry["attempts"] += 1
        if isinstance(row.get("tokens"), int) and not isinstance(row.get("tokens"), bool):
            entry["tokens"].append(row["tokens"])
        entry["tasks"].setdefault(row["task"], row["bucket"])
    return out


def _decided(entry):
    buckets = list(entry["tasks"].values())
    decided = sum(bucket in DECIDED for bucket in buckets)
    return decided, buckets.count("accepted")


def role_fold_audit(rows, min_samples=DEFAULT_MIN_SAMPLES, roles=None):
    """Per-role use in the window plus role pairs that almost always share a task.

    Nominates only: the reported history cannot show that a folded version would measure worse.
    """
    roles = known_roles() if roles is None else roles
    tally = _tally(rows, lambda row: _clean(row.get("role"), "unknown"))
    per_role = []
    for role, entry in sorted(tally.items()):
        decided, admitted = _decided(entry)
        per_role.append({"role": role, "attempts": entry["attempts"], "tasks": len(entry["tasks"]),
                         "decided": decided, "admitted": admitted,
                         "admit_rate": round(admitted / decided, 4) if decided >= min_samples else None})
    names = sorted(role for role in tally if role != "unknown")
    pairs = []
    for index, first in enumerate(names):
        for second in names[index + 1:]:
            one, two = set(tally[first]["tasks"]), set(tally[second]["tasks"])
            rarer, shared = min(len(one), len(two)), len(one & two)
            if rarer >= min_samples and shared / rarer >= FOLD_CO_OCCURRENCE:
                pairs.append({"roles": [first, second], "shared_tasks": shared, "rarer_role_tasks": rarer,
                              "co_occurrence": round(shared / rarer, 4)})
    return {"min_samples": min_samples, "co_occurrence_threshold": FOLD_CO_OCCURRENCE,
            "roles": per_role, "never_used": sorted(set(roles) - set(tally)), "pairs": pairs,
            "note": "fold candidates (reported history, not a decision); thresholds are estimates; "
                    "fold one at a time and keep a role only if the folded version measures worse"}


def model_preference(rows, min_samples=DEFAULT_MIN_SAMPLES):
    """Per role, provider/model groups side by side when at least two have enough decided tasks."""
    tally = _tally(rows, lambda row: (_clean(row.get("role"), "unknown"),
                                      _clean(row.get("provider"), "unknown"), _clean(row.get("model"), "unknown")))
    by_role = {}
    for (role, provider, model), entry in sorted(tally.items()):
        decided, admitted = _decided(entry)
        if decided < min_samples:
            continue
        by_role.setdefault(role, []).append({
            "provider": provider, "model": model, "decided": decided, "admitted": admitted,
            "admit_rate": round(admitted / decided, 4),
            "median_tokens": statistics.median(entry["tokens"]) if entry["tokens"] else None})
    roles = {}
    for role, groups in by_role.items():
        if len(groups) < 2:
            continue
        groups.sort(key=lambda item: (-item["admit_rate"], -item["decided"], item["provider"], item["model"]))
        gap = round(groups[0]["admit_rate"] - groups[-1]["admit_rate"], 4)
        roles[role] = {"groups": groups, "gap": gap, "clear": gap >= MODEL_GAP_NOISE}
    return {"min_samples": min_samples, "noise_threshold": MODEL_GAP_NOISE, "roles": roles,
            "note": "reported history, not a decision; the noise threshold is an estimate"}


def fold_text(report):
    lines = []
    audit = report.get("role_fold_audit")
    if audit:
        lines.append("fold candidates (reported history, not a decision; thresholds are estimates):")
        for item in audit["roles"]:
            rate = "n<%d" % audit["min_samples"] if item["admit_rate"] is None else "%.1f%%" % (item["admit_rate"] * 100)
            lines.append("fold role %s: attempts=%d decided=%d admit=%s" % (
                item["role"], item["attempts"], item["decided"], rate))
        lines.append("fold never used in window: %s" % (", ".join(audit["never_used"]) or "none"))
        for pair in audit["pairs"]:
            lines.append("fold pair %s + %s: same task %d/%d (%.0f%%, threshold %.0f%%)" % (
                pair["roles"][0], pair["roles"][1], pair["shared_tasks"], pair["rarer_role_tasks"],
                pair["co_occurrence"] * 100, audit["co_occurrence_threshold"] * 100))
        if not audit["pairs"]:
            lines.append("fold pairs: none")
    preference = report.get("model_preference")
    if preference:
        for role, entry in preference["roles"].items():
            side = "; ".join("%s/%s admit %.1f%% over %d decided median_tokens=%s" % (
                g["provider"], g["model"], g["admit_rate"] * 100, g["decided"],
                "none" if g["median_tokens"] is None else "%g" % g["median_tokens"]) for g in entry["groups"])
            verdict = ("gap %.1f points" % (entry["gap"] * 100)) if entry["clear"] else "no clear difference (gap %.1f points)" % (entry["gap"] * 100)
            lines.append("model preference %s: %s; %s" % (role, side, verdict))
        if not preference["roles"]:
            lines.append("model preference: no role has two groups with >= %d decided" % preference["min_samples"])
    return lines


def _patch_sha(row):
    sha = row.get("patch_sha256")
    external = row.get("patch_external")
    if not sha and isinstance(external, dict):
        sha = external.get("sha256")
    return sha if isinstance(sha, str) and sha else None


def claim_check(index_rows):
    """Per role/provider/model: how often a worker's own "tests pass" claim met a failing admission re-run.

    Joins delegate rows that recorded `worker_claim: pass` to patch-admit rows by patch digest. Only an
    admission that actually re-ran verification (numeric verify_exit) counts as `rerun`; the latest one wins.
    A count, never a verdict: nothing here changes admission or routing.
    """
    rerun = {}
    for row in index_rows:
        if isinstance(row, dict) and row.get("kind") == "patch-admit" and _patch_sha(row) \
                and isinstance(row.get("verify_exit"), int) and not isinstance(row.get("verify_exit"), bool):
            rerun[_patch_sha(row)] = row["verify_exit"]
    groups = {}
    for row in index_rows:
        if not isinstance(row, dict) or row.get("kind") != "delegate" or row.get("worker_claim") != "pass":
            continue
        key = (_clean(row.get("role"), "unknown"), _clean(row.get("provider"), "unknown"),
               _clean(row.get("selected_model") or row.get("requested_model"), "unknown"))
        entry = groups.setdefault(key, {"claimed_pass": 0, "rerun": 0, "disagreed": 0})
        entry["claimed_pass"] += 1
        sha = _patch_sha(row)
        if sha in rerun:
            entry["rerun"] += 1
            entry["disagreed"] += rerun[sha] != 0
    return {"groups": [dict(role=k[0], provider=k[1], model=k[2], **v) for k, v in sorted(groups.items())],
            "note": "worker said tests pass vs the parent's admission re-run; counts, not a verdict"}


def claim_text(report):
    check = report.get("claim_check") or {}
    return ["claim %s %s/%s: claimed_pass=%d rerun=%d disagreed=%d (counts, not a verdict)" % (
        g["role"], g["provider"], g["model"], g["claimed_pass"], g["rerun"], g["disagreed"])
        for g in check.get("groups", [])]


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
    lines.extend(claim_text(report))
    lines.extend(fold_text(report))
    return "\n".join(lines)
