"""Compact orientation brief cached beside the graph at build time.

Kept apart from render.py: render imports analytics, and project code must not
import render back.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any, Dict, Mapping

from oms_runtime.common import atomic_write_json, read_json, utc_now

from . import analytics
from .query import Graph

BRIEF_BYTES = 1536
FILE_HUBS = 6
AREAS = 3
FILENAME = "orientation.json"


def _plain(value: Any) -> str:
    """Keep repository-derived labels on one printable line."""
    return " ".join("".join(ch if ch.isprintable() else " " for ch in str(value or "")).split())


def _areas(areas: Mapping[str, Any]) -> str:
    rows = sorted(areas.items(), key=lambda item: (-item[1]["files"], item[0]))
    parts = []
    for name, row in rows[:AREAS]:
        languages = sorted(row["languages"].items(), key=lambda item: (-item[1], item[0]))
        mix = ", ".join("%s %d" % pair for pair in languages[:2]) if len(languages) > 1 else ""
        parts.append("%s %d%s" % (_plain(name), row["files"], " (%s)" % mix if mix else ""))
    if len(rows) > AREAS:
        parts.append("+%d" % (len(rows) - AREAS))
    return " · ".join(parts)


def brief_text(summary: Mapping[str, Any], manifest: Mapping[str, Any], repo_name: str) -> str:
    """About six lines, whole lines only, never over BRIEF_BYTES UTF-8 bytes."""
    stats = manifest.get("stats") if isinstance(manifest.get("stats"), Mapping) else {}
    counts = (summary.get("assurance") or {}).get("counts") or {}
    need = int(counts.get("needs-evidence", 0)) + int(counts.get("attention", 0))
    built = str(manifest.get("generated_at", ""))[:16].replace("T", " ")
    lines = ["[oms graph] %s revision %s · built %s UTC · %d files, %d nodes" % (
        _plain(repo_name), str(summary.get("revision", ""))[:12], built or "unknown",
        int(stats.get("files", 0)), int(stats.get("nodes", 0)))]
    if summary.get("areas"):
        lines.append("areas: " + _areas(summary["areas"]))
    hubs = ["%s %d" % (_plain(str(item["id"]).split(":", 1)[-1]), item["degree"])
            for item in summary.get("file_hubs", [])[:FILE_HUBS]]
    if hubs:
        lines.append("file hubs: " + " · ".join(hubs))
    if need:
        lines.append("attention: %d nodes need structural evidence (oms graph project map)" % need)
    lines.append('ask the graph: oms graph project find NAME · api PATH · blast --path PATH · context --task "..."')
    lines.append("(repeats only when the graph revision changes; the graph refreshes in the background)")
    kept, size = [], 0
    for line in lines:
        size += len(line.encode("utf-8")) + (1 if kept else 0)
        if size > BRIEF_BYTES:
            break
        kept.append(line)
    return "\n".join(kept)


def summarize(graph: Mapping[str, Any]) -> Dict[str, Any]:
    """Test-free overview plus the assurance counts the brief quotes."""
    summary = Graph(dict(graph)).without_tests().map_summary()
    assurance = analytics.structural_assurance(graph.get("nodes", []), graph.get("edges", []), include_tests=False)
    summary["assurance"] = {"counts": assurance["counts"]}
    return summary


def write(state_dir: Path, graph: Mapping[str, Any], manifest: Mapping[str, Any], repo_name: str = "") -> Dict[str, Any]:
    payload = {"schema": 1, "revision": str(manifest.get("revision", "")), "generated_at": utc_now(),
               "text": brief_text(summarize(graph), manifest, repo_name or Path(state_dir).resolve().parent.parent.name)}
    atomic_write_json(Path(state_dir) / FILENAME, payload)
    return payload


def read(state_dir: Path, revision: str) -> str:
    """Cached text when it belongs to this graph revision, else empty."""
    payload = read_json(Path(state_dir) / FILENAME, None)
    if isinstance(payload, dict) and payload.get("revision") == revision and isinstance(payload.get("text"), str):
        return payload["text"]
    return ""
